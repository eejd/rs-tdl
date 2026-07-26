# TIDAL API compatibility and rate-limit research

Last updated: 2026-07-26

## Scope

This note records findings from restoring `rust-tdl` against TIDAL's current
authentication, metadata, and playback behavior. It intentionally does not
record OAuth client secrets, access tokens, refresh tokens, signed media URLs,
or other credentials.

## Tracking issues

- [#1 Skip existing files before requesting playback manifests](https://github.com/eejd/rust-tdl/issues/1)
- [#2 Coordinate HTTP 429 backoff and pace playback requests globally](https://github.com/eejd/rust-tdl/issues/2)
- [#3 Support OAuth client profiles and current TIDAL session workflow](https://github.com/eejd/rust-tdl/issues/3)
- [#4 Make output atomic and metadata handling format-aware](https://github.com/eejd/rust-tdl/issues/4)

## Confirmed observations

### Authentication and session state

- Current device authorization accepts `client_id` and `client_secret` as form
  fields. Existing access and refresh tokens are bound to the OAuth client that
  issued them, so changing clients requires a fresh device authorization.
- `/v1/sessions` returns an ephemeral `sessionId`. Metadata requests can work
  without preserving it, while playback requests can fail; `rust-tdl` now
  persists and supplies it.
- The current local implementation uses `https://api.tidal.com/v1`, an Android
  WebView user agent, and `x-tidal-client-version: 2025.7.16`.
- Both the widely shared `tidalapi` OAuth client and a newer client used by a
  local TIDAL implementation authorized successfully. A newly linked token for
  the newer client still received HTTP 429 from playback.

### Playback and quality

- Playlist and track metadata can advertise `LOSSLESS` while a playback request
  returns a 320 kbps AAC stream. The downloader must validate the playback
  response and manifest codec rather than trusting catalog metadata.
- A downloaded sample reported by macOS `afinfo` was AAC, 44.1 kHz, 320 kbps in
  an MP4 container. Passing that file to `metaflac` produced `InvalidInput:
  reader does not contain flac metadata`.
- `audio/mp4` describes a container, not necessarily whether audio is lossy.
  Codec plus returned quality must determine acceptance. For a configured
  `LOSSLESS` request, AAC is a downgrade and must be rejected.
- Playback manifests contain short-lived signed media URLs and must not use the
  persistent metadata cache.

### HTTP 429 behavior

- A single uncached request to
  `/tracks/{id}/playbackinfopostpaywall` returned a live HTTP 429 with an empty
  body and no `Retry-After` header.
- Official TIDAL playback and a DJ Pro integration continued to work for the
  same account. This narrows the failure to some combination of public IP,
  OAuth client identity, unofficial-client policy, request fingerprint, or a
  playback-endpoint-specific quota.
- Changing OAuth credentials, relinking the device, preserving `sessionId`,
  changing the API hostname, and aligning Android client headers did not clear
  the immediate 429.
- A public-IP cooldown remains plausible because diagnostic playlist runs may
  have amplified requests through independent per-track retries.

## Why a playlist download can amplify requests

Listing a playlist only retrieves metadata. `tdl get <playlist>`, however,
downloads every item and therefore obtains a playback manifest for each track:

```text
playlist items
    -> track metadata
    -> album and artist metadata
    -> playbackinfopostpaywall
    -> signed CDN media URL
    -> media download
```

For a 99-track playlist, that is normally 99 playback-manifest requests. The
old middleware allowed five retries after the initial request. If every request
failed transiently, the theoretical maximum was `99 * 6 = 594` playback calls
for one run.

The existing-file check also happened after fetching the manifest because the
manifest selected the file extension. Consequently, files that were already on
disk still consumed playback requests before being skipped.

Official clients generally fetch playlist metadata first and obtain signed
playback URLs just in time for the current track and a small playback buffer.
They do not need a signed URL for every track merely to display a playlist.

## Required downloader behavior

### Skip before playback

- Derive expected candidate paths before requesting a manifest.
- For `LOSSLESS`, check at least the expected `.flac` destination before
  playback. Account for supported lossless MP4/ALAC packaging explicitly.
- Validate existing files where practical; a final filename alone must not make
  a partial or invalid file permanently skippable.
- Only request playback information for genuinely missing tracks.

### Coordinated rate limiting

- Limit playback-manifest concurrency independently from metadata and media
  transfer concurrency; default it to one.
- Add configurable random spacing between playback requests. Historical
  tidal-dl behavior used approximately 0.5-5 seconds; the newer local client
  defaults to 3-5 seconds between downloads.
- Honor `Retry-After` when present.
- Use exponential backoff with jitter when it is absent, with configurable caps.
- Treat 429 as a global signal for the shared TIDAL client/session: stop
  scheduling new playback requests and apply one shared cooldown. Do not let
  every queued track run an independent retry loop.
- Bound total attempts and print the selected delay, attempt count, endpoint,
  and affected track.

### Atomic, format-aware output

- Download to a same-directory partial path and only rename to the final path
  after the transfer and metadata write succeed.
- Dispatch metadata handling by actual format. `metaflac` is valid only for
  FLAC; MP4/M4A requires an MP4-aware tag writer.
- Preserve failed partial files under an unmistakable partial name or clean
  them up deliberately. Never leave a failed file at its final path where the
  next run will skip it.
- Keep all errors visible even when progress rendering is disabled.

### OAuth client profiles

- Continue supporting user-supplied `client_id` and `client_secret` values.
- Support named profiles rather than silently rotating public credentials.
- Store no secrets in source control or diagnostic output.
- Clear client-bound tokens and require relinking when the selected profile
  changes.
- A personal application from TIDAL's developer dashboard is preferable for
  supported catalog/account APIs, but access to the private playback endpoint
  must be tested and must not be assumed.

## Controlled diagnostic matrix

After allowing the current public IP to cool down, make exactly one playback
request per test condition:

| Official playback | Network | `tdl` result | Likely interpretation |
| --- | --- | --- | --- |
| Works | Existing IP | 429 | IP, client, fingerprint, or unofficial policy |
| Works | Different residential IP | Works | Existing public IP was throttled |
| Works | Different residential IP | 429 | OAuth client, account, fingerprint, or unofficial policy |
| Fails | Any | Fails | Account, service, network, or regional playback problem |

Prefer a residential hotspot over a commercial VPN, whose exit address may
already be rate-limited. Do not rotate repeatedly or retry continuously; that
would obscure the result and may extend the cooldown.

## Sources

- [TIDAL: Manage apps](https://developer.tidal.com/documentation/api-sdk/api-sdk-manage-apps)
- [TIDAL: Authorization](https://developer.tidal.com/documentation/api-sdk/api-sdk-authorization)
- [TIDAL: Playback issues](https://support.tidal.com/hc/en-us/articles/360003337357-Playback-Issues)
- [MDN: HTTP 429 Too Many Requests](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Status/429)
- [Public TIDAL API-key history gist](https://gist.github.com/yaronzz/48d01f5a24b4b7b37f19443977c22cd6)
- [Tidal-Media-Downloader PR #840: client change and relinking](https://github.com/yaronzz/Tidal-Media-Downloader/pull/840)
- [Tidal-Media-Downloader discussion #1208: late-2025/2026 API breakage](https://github.com/yaronzz/Tidal-Media-Downloader/discussions/1208)
- [Roon report: 429 from playbackinfopostpaywall](https://community.roonlabs.com/t/random-track-stops-during-streaming-playback-ref-ikldhw/280047)
- [Lyrion community playback logs](https://forums.lyrion.org/forum/user-forums/3rd-party-software/1763135-tidal-streams-stopping-frequently)
- [Minim TIDAL playback API reference](https://minim.readthedocs.io/en/latest/_modules/minim/tidal.html)
