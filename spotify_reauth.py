#!/usr/bin/env python3
"""Mint a new SPOTIFY_REFRESH_TOKEN.

Refresh tokens can be revoked (Spotify answers the token endpoint with
`{"error": "invalid_grant", "error_description": "Refresh token revoked"}`).
When that happens the only fix is to walk the authorization-code flow again as
the account owner. This script does that without hardcoding a single credential:
everything is read from .env or the environment, and only the new refresh token
is printed.

Usage:
    python3 spotify_reauth.py            # print the authorize URL, then prompt
    python3 spotify_reauth.py --write-env  # also write the token into .env

Requires SPOTIFY_CLIENT_ID and SPOTIFY_CLIENT_SECRET to already be set, and the
redirect URI below to be registered on the app in the Spotify dashboard.
"""

import argparse
import base64
import json
import os
import shutil
import sys
import urllib.parse
import urllib.request
import urllib.error

ENV_PATH = ".env"
TOKEN_URL = "https://accounts.spotify.com/api/token"
AUTHORIZE_URL = "https://accounts.spotify.com/authorize"
DEFAULT_REDIRECT_URI = "https://jeaic.com"

# The only scope /v1/me/player/recently-played needs. Asking for more would mean
# a broader consent screen and a token that can do more than this service should.
SCOPE = "user-read-recently-played"


def load_env(path=ENV_PATH):
    """Read KEY=VALUE pairs from .env without overriding the real environment."""
    values = {}
    if not os.path.exists(path):
        return values
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, _, value = line.partition("=")
            values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def require(name, env_file):
    value = os.environ.get(name) or env_file.get(name)
    if not value:
        sys.exit(
            f"error: {name} is not set. Put it in {ENV_PATH} or export it before running."
        )
    return value


def post_token(payload, client_id, client_secret):
    basic = base64.b64encode(f"{client_id}:{client_secret}".encode()).decode()
    request = urllib.request.Request(
        TOKEN_URL,
        data=urllib.parse.urlencode(payload).encode(),
        headers={
            "Authorization": f"Basic {basic}",
            "Content-Type": "application/x-www-form-urlencoded",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.loads(response.read().decode())
    except urllib.error.HTTPError as error:
        body = error.read().decode(errors="replace")
        sys.exit(f"error: token endpoint returned HTTP {error.code}\n{body}")
    except urllib.error.URLError as error:
        sys.exit(f"error: could not reach {TOKEN_URL}: {error.reason}")


def extract_code(raw):
    """Accept either a bare code or the whole redirect URL pasted from the bar."""
    raw = raw.strip()
    if not raw:
        return None
    if raw.startswith("http://") or raw.startswith("https://"):
        query = urllib.parse.urlparse(raw).query
        params = urllib.parse.parse_qs(query)
        if "error" in params:
            sys.exit(f"error: Spotify returned '{params['error'][0]}' - authorization denied.")
        codes = params.get("code")
        if not codes:
            sys.exit("error: no 'code' parameter in that URL.")
        return codes[0]
    return raw


def write_env(token, path=ENV_PATH):
    """Replace SPOTIFY_REFRESH_TOKEN in .env, keeping a backup and the file mode."""
    if not os.path.exists(path):
        sys.exit(f"error: {path} does not exist; cannot write to it.")

    shutil.copy2(path, path + ".bak")

    with open(path, "r", encoding="utf-8") as handle:
        lines = handle.readlines()

    replaced = False
    for index, line in enumerate(lines):
        if line.strip().startswith("SPOTIFY_REFRESH_TOKEN="):
            lines[index] = f"SPOTIFY_REFRESH_TOKEN={token}\n"
            replaced = True
            break
    if not replaced:
        if lines and not lines[-1].endswith("\n"):
            lines.append("\n")
        lines.append(f"SPOTIFY_REFRESH_TOKEN={token}\n")

    with open(path, "w", encoding="utf-8") as handle:
        handle.writelines(lines)

    print(f"wrote SPOTIFY_REFRESH_TOKEN to {path} (backup at {path}.bak)")


def main():
    parser = argparse.ArgumentParser(description="Mint a new Spotify refresh token")
    parser.add_argument(
        "--redirect-uri",
        default=os.environ.get("SPOTIFY_REDIRECT_URI", DEFAULT_REDIRECT_URI),
        help=f"Must match a redirect URI registered on the app (default: {DEFAULT_REDIRECT_URI})",
    )
    parser.add_argument(
        "--write-env",
        action="store_true",
        help=f"Write the new token into {ENV_PATH} instead of only printing it",
    )
    args = parser.parse_args()

    env_file = load_env()
    client_id = require("SPOTIFY_CLIENT_ID", env_file)
    client_secret = require("SPOTIFY_CLIENT_SECRET", env_file)

    authorize = AUTHORIZE_URL + "?" + urllib.parse.urlencode({
        "client_id": client_id,
        "response_type": "code",
        "redirect_uri": args.redirect_uri,
        "scope": SCOPE,
    })

    print("1. Open this URL as the Spotify account that owns the listening history:\n")
    print(f"   {authorize}\n")
    print("2. Approve the request. The browser lands on a URL containing ?code=...")
    print("   (the page itself may 404 - that is fine, only the URL matters).\n")
    print("3. Paste the full redirect URL, or just the code, below.")
    print("   Authorization codes expire in about a minute, so do this promptly.\n")

    code = extract_code(input("code or redirect URL> "))
    if not code:
        sys.exit("error: nothing entered.")

    result = post_token(
        {
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": args.redirect_uri,
        },
        client_id,
        client_secret,
    )

    refresh_token = result.get("refresh_token")
    if not refresh_token:
        sys.exit(f"error: no refresh_token in the response: {json.dumps(result)[:400]}")

    granted = result.get("scope", "")
    if SCOPE not in granted:
        print(
            f"\nwarning: the granted scope is '{granted}', which does not include "
            f"'{SCOPE}'. Recently-played will 403 with this token.",
            file=sys.stderr,
        )

    print("\nSuccess. New refresh token:\n")
    print(refresh_token)
    print("\nGranted scope:", granted or "(none reported)")

    if args.write_env:
        print()
        write_env(refresh_token)
        print("Restart the service to pick it up, e.g.:")
        print("  sudo systemctl restart api-aggregator.service")
    else:
        print("\nSet it as SPOTIFY_REFRESH_TOKEN in .env, then restart the service.")
        print("Do not commit it. Re-run with --write-env to have this script do it.")


if __name__ == "__main__":
    main()
